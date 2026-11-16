# Testing Guide for HyperStudy Bridge

This document provides comprehensive information about the testing setup, structure, and execution for the HyperStudy Bridge application.

## Test Structure

The test suite is organized into several categories:

### 1. Component Tests (`src/lib/components/tests/`)

- **DeviceCard.test.js**: Tests the device card component including status indicators, connection actions, and configuration modal triggers
- **AddDeviceModal.test.js**: Tests the device selection modal with multi-select functionality and keyboard navigation
- **DeviceConfigModal.test.js**: Tests device-specific configuration forms with validation for each device type

### 2. Store Tests (`src/lib/stores/tests/`)

- **websocket.test.js**: Tests WebSocket connection management, message handling, and device operations
- **logs.test.js**: Tests log management, filtering, export functionality, and circular buffer behavior

## Test Technologies

- **Vitest**: Primary testing framework
- **@testing-library/svelte**: Component testing utilities
- **@testing-library/user-event**: User interaction simulation
- **@testing-library/jest-dom**: Additional DOM matchers
- **jsdom**: DOM environment for tests

## Configuration Files

### vitest.config.mjs
Main Vitest configuration with Svelte plugin support:
```javascript
export default defineConfig({
  plugins: [svelte({ hot: !process.env.VITEST, configFile: './svelte.config.js' })],
  test: {
    globals: true,
    environment: 'jsdom',
    setupFiles: ['src/test/setup.js'],
    coverage: {
      provider: 'v8',
      thresholds: { global: { branches: 70, functions: 70, lines: 70, statements: 70 } }
    }
  }
});
```

### src/test/setup.js
Test setup file with mocks and global configurations:
- Mocks Tauri API calls
- Mocks WebSocket for testing
- Configures console methods
- Imports jest-dom matchers

### svelte.config.js
Svelte configuration for tests:
```javascript
export default {
  preprocess: vitePreprocess(),
  compilerOptions: { runes: true }
};
```

## Running Tests

### Prerequisites

1. Install test dependencies:
```bash
npm install --save-dev @testing-library/svelte @testing-library/jest-dom @testing-library/user-event
```

2. Ensure Svelte 5 runes compilation is properly configured

### Test Commands

```bash
# Run all tests
npm test

# Run tests in watch mode
npm run test:watch

# Run tests with coverage
npm run test:coverage

# Run specific test file
npx vitest run src/lib/components/tests/DeviceCard.test.js

# Run component tests only
npx vitest run src/lib/components/tests/
```

## Test Coverage Areas

### DeviceCard Component
- ✅ Component rendering with device information
- ✅ Status indicator colors and labels
- ✅ Connection/disconnection actions
- ✅ Configuration modal triggers
- ✅ Error state handling
- ✅ Accessibility features
- ✅ Responsive design
- ✅ Keyboard navigation

### AddDeviceModal Component
- ✅ Modal visibility and structure
- ✅ Available device display
- ✅ Single and multi-device selection
- ✅ Keyboard shortcuts (Cmd/Ctrl+Click)
- ✅ Form validation
- ✅ Modal actions (Add, Cancel, Close)
- ✅ Keyboard navigation (Escape, Tab)
- ✅ Accessibility compliance

### DeviceConfigModal Component
- ✅ Device-specific configuration forms
- ✅ TTL device validation (port format, pulse duration)
- ✅ Kernel device validation (IP address, port range)
- ✅ Pupil device validation (URL format, checkboxes)
- ✅ LSL configuration tab
- ✅ Form submission and error handling
- ✅ Unsaved changes detection
- ✅ Accessibility features

### WebSocket Store
- ✅ Connection management
- ✅ Message parsing and handling
- ✅ Device state synchronization
- ✅ Automatic reconnection logic
- ✅ Command sending (TTL via Tauri, others via WebSocket)
- ✅ Error handling and recovery
- ✅ Performance with rapid updates
- ✅ Request-response correlation

### Logs Store
- ✅ Log entry creation and management
- ✅ Circular buffer behavior
- ✅ Filtering by level, device, and search query
- ✅ Backend log fetching and deduplication
- ✅ Log export functionality
- ✅ Polling mechanism
- ✅ Statistics generation
- ✅ Performance with large datasets

### Integration Tests
- ✅ Full application initialization
- ✅ Multi-device management
- ✅ Device connection workflows
- ✅ Configuration persistence
- ✅ Log viewer integration
- ✅ Settings panel functionality
- ✅ Real-time updates
- ✅ Error handling across components
- ✅ Accessibility compliance
- ✅ Performance under load

## Mocking Strategy

### Tauri API
```javascript
vi.mock('@tauri-apps/api', () => ({
  invoke: vi.fn(),
  listen: vi.fn(),
  emit: vi.fn(),
}));
```

### WebSocket
```javascript
global.WebSocket = vi.fn(() => ({
  send: vi.fn(),
  close: vi.fn(),
  addEventListener: vi.fn(),
  removeEventListener: vi.fn(),
  readyState: 1,
}));
```

### Stores
Stores are mocked to return controlled data for predictable testing:
```javascript
vi.mock('../lib/stores/websocket.svelte.js', () => ({
  getDevices: vi.fn(() => new Map()),
  connectDevice: vi.fn(),
  disconnectDevice: vi.fn(),
}));
```

## Known Issues and Workarounds

### Svelte 5 Runes in Tests

**Issue**: Svelte 5 runes (`$state`, `$derived`, `$effect`) are not available in the test environment by default.

**Workaround**: Tests use mocked versions of store functions rather than testing the rune-based reactive state directly.

### WebSocket Testing

**Issue**: Real WebSocket connections cannot be established in test environment.

**Solution**: WebSocket is mocked with a function that returns an object with the expected methods.

### Component Testing with Stores

**Issue**: Svelte components using stores need the store context to be properly mocked.

**Solution**: All stores are mocked at the module level to provide predictable data.

## Test Patterns

### Component Testing Pattern
```javascript
describe('ComponentName', () => {
  const mockProps = { /* props */ };

  beforeEach(() => {
    vi.clearAllMocks();
  });

  it('renders correctly', () => {
    render(ComponentName, mockProps);
    expect(screen.getByText('Expected Text')).toBeInTheDocument();
  });

  it('handles user interaction', async () => {
    const user = userEvent.setup();
    render(ComponentName, mockProps);

    await user.click(screen.getByText('Button'));
    expect(mockFunction).toHaveBeenCalled();
  });
});
```

### Store Testing Pattern
```javascript
describe('StoreName', () => {
  beforeEach(() => {
    // Reset store state
    clearFunction();
    vi.clearAllMocks();
  });

  it('manages state correctly', () => {
    const initialState = getState();
    expect(initialState).toEqual(expectedInitialState);

    updateState(newValue);
    expect(getState()).toEqual(expectedNewState);
  });
});
```

## Future Enhancements

### Playwright Integration

For true end-to-end testing, consider adding Playwright tests that:
- Test the actual Tauri application
- Verify real device connections
- Test cross-platform behavior
- Validate performance under real conditions

### Visual Regression Testing

Add visual regression tests to catch UI changes:
- Component snapshots
- Responsive design validation
- Theme consistency checks

### Performance Testing

Add dedicated performance tests:
- Large dataset handling
- Memory usage validation
- Connection speed testing
- UI responsiveness metrics

## Debugging Tests

### Enable Console Output
Uncomment console mocking in `src/test/setup.js`:
```javascript
// log: vi.fn(),  // Comment out to see console output
```

### Run Tests in Debug Mode
```bash
npx vitest run --reporter=verbose
```

### Use Browser DevTools
```bash
npx vitest --ui
```

## Continuous Integration

Recommended CI configuration:
```yaml
- name: Run Tests
  run: |
    npm test
    npm run test:coverage
- name: Upload Coverage
  uses: codecov/codecov-action@v3
```

This comprehensive test suite provides excellent coverage of the HyperStudy Bridge frontend functionality, ensuring reliability and maintainability of the application.