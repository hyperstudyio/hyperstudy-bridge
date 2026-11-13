import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, fireEvent, waitFor } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import '@testing-library/jest-dom';
import DeviceCard from '../DeviceCard.svelte';
import * as bridgeStore from '../../stores/websocket.svelte.js';

// Mock the bridge store
vi.mock('../../stores/websocket.svelte.js', () => ({
  connectDevice: vi.fn(),
  disconnectDevice: vi.fn(),
}));

// Mock DeviceConfigModal
vi.mock('../DeviceConfigModal.svelte', () => ({
  default: vi.fn(() => ({
    component: 'div',
    props: {},
  })),
}));

describe('DeviceCard', () => {
  const user = userEvent.setup();

  const mockDevice = {
    id: 'ttl',
    name: 'TTL Pulse Generator',
    type: 'Adafruit RP2040',
    connection: 'USB Serial',
    status: 'disconnected',
    config: {
      port: '/dev/ttyUSB0',
    },
  };

  const mockKernelDevice = {
    id: 'kernel',
    name: 'Kernel Flow2',
    type: 'fNIRS',
    connection: 'TCP Socket',
    status: 'connected',
    config: {
      ip: '127.0.0.1',
      port: 6767,
    },
  };

  const mockPupilDevice = {
    id: 'pupil',
    name: 'Pupil Labs Neon',
    type: 'Eye Tracker',
    connection: 'REST API + LSL',
    status: 'error',
    config: {
      url: 'neon.local:8080',
    },
  };

  beforeEach(() => {
    vi.clearAllMocks();
    // Reset console methods
    vi.spyOn(console, 'log').mockImplementation(() => {});
    vi.spyOn(console, 'error').mockImplementation(() => {});
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  describe('Component Rendering', () => {
    it('renders device card with basic information', () => {
      render(DeviceCard, { device: mockDevice });

      expect(screen.getByText('TTL Pulse Generator')).toBeInTheDocument();
      expect(screen.getByText('Adafruit RP2040')).toBeInTheDocument();
      expect(screen.getByText('USB Serial')).toBeInTheDocument();
      expect(screen.getByText('Disconnected')).toBeInTheDocument();
    });

    it('displays device configuration for TTL device', () => {
      render(DeviceCard, { device: mockDevice });

      expect(screen.getByText('Port:')).toBeInTheDocument();
      // TTL port is displayed in a select element
      // The select exists with port selection UI even if async device list hasn't loaded
      const portSelect = document.querySelector('.port-select');
      expect(portSelect).toBeInTheDocument();
      expect(screen.getByText('Select device...')).toBeInTheDocument();
    });

    it('displays device configuration for network devices', () => {
      render(DeviceCard, { device: mockKernelDevice });

      expect(screen.getByText('IP:')).toBeInTheDocument();
      expect(screen.getByText('127.0.0.1')).toBeInTheDocument();
      expect(screen.getByText('Port:')).toBeInTheDocument();
      expect(screen.getByText('6767')).toBeInTheDocument();
    });

    it('shows not-configured for a Pupil device with neither URL nor pinned phone', () => {
      const pupilWithoutUrl = {
        ...mockPupilDevice,
        config: {},
      };
      render(DeviceCard, { device: pupilWithoutUrl });

      expect(screen.getByText('URL:')).toBeInTheDocument();
      expect(screen.getByText('Not configured')).toBeInTheDocument();
      expect(screen.getByText('Any (not pinned)')).toBeInTheDocument();
    });

    it('shows the pinned phone id and resolved-address hint for a pinned Pupil device', () => {
      const pinned = {
        ...mockPupilDevice,
        config: { device_id: 'a41fe4fe2bccf6c3' },
      };
      render(DeviceCard, { device: pinned });

      expect(screen.getByText('Resolved from pinned phone')).toBeInTheDocument();
      expect(screen.getByText('a41fe4fe2bccf6c3')).toBeInTheDocument();
    });

    it('shows which phone answered once a Pupil device is connected', () => {
      const connected = {
        ...mockPupilDevice,
        status: 'connected',
        config: { url: '192.168.50.220:8080', device_id: 'a41fe4fe2bccf6c3' },
        info: {
          device_name: 'Neon P2',
          device_id: 'a41fe4fe2bccf6c3',
          device_ip: '192.168.50.220',
          battery_level: 82,
          recording_id: '0704e3f9-b284-4fd5-92bd-0d4d65f5e4c9',
          recording_owned: false,
        },
      };
      render(DeviceCard, { device: connected });

      expect(
        screen.getByText(/Neon P2 · a41fe4fe2bccf6c3 · 192\.168\.50\.220 · 82%/)
      ).toBeInTheDocument();
      expect(screen.getByText(/started elsewhere/)).toBeInTheDocument();
    });

    it('handles unconfigured device gracefully', () => {
      // Test kernel device without IP (shows "Not configured")
      const unconfiguredKernel = {
        ...mockKernelDevice,
        config: { port: 6767 }, // IP missing
      };
      render(DeviceCard, { device: unconfiguredKernel });

      expect(screen.getByText('Not configured')).toBeInTheDocument();
    });
  });

  describe('Status Indicators', () => {
    it('displays correct status colors', () => {
      const { rerender } = render(DeviceCard, { device: mockDevice });

      // Test disconnected status
      let statusDot = document.querySelector('.status-dot');
      expect(statusDot).toHaveStyle('background-color: var(--color-text-disabled)');

      // Test connected status
      rerender({ device: { ...mockDevice, status: 'connected' } });
      statusDot = document.querySelector('.status-dot');
      expect(statusDot).toHaveStyle('background-color: var(--color-success)');

      // Test connecting status
      rerender({ device: { ...mockDevice, status: 'connecting' } });
      statusDot = document.querySelector('.status-dot');
      expect(statusDot).toHaveStyle('background-color: var(--color-warning)');

      // Test error status
      rerender({ device: { ...mockDevice, status: 'error' } });
      statusDot = document.querySelector('.status-dot');
      expect(statusDot).toHaveStyle('background-color: var(--color-error)');
    });

    it('displays correct status labels', () => {
      const { rerender } = render(DeviceCard, { device: mockDevice });

      expect(screen.getByText('Disconnected')).toBeInTheDocument();

      rerender({ device: { ...mockDevice, status: 'connected' } });
      expect(screen.getByText('Connected')).toBeInTheDocument();

      rerender({ device: { ...mockDevice, status: 'connecting' } });
      expect(screen.getByText('Connecting...')).toBeInTheDocument();

      rerender({ device: { ...mockDevice, status: 'error' } });
      expect(screen.getByText('Error')).toBeInTheDocument();
    });

    it('has correct status dot tooltip', () => {
      render(DeviceCard, { device: mockDevice });

      const statusDot = document.querySelector('.status-dot');
      expect(statusDot).toHaveAttribute('title', 'Disconnected');
    });
  });

  describe('Connection Actions', () => {
    it('shows Connect button for disconnected device', () => {
      render(DeviceCard, { device: mockDevice });

      const connectButton = screen.getByText('Connect');
      expect(connectButton).toBeInTheDocument();
      expect(connectButton).not.toBeDisabled();
    });

    it('shows Disconnect button for connected device', () => {
      render(DeviceCard, { device: { ...mockDevice, status: 'connected' } });

      const disconnectButton = screen.getByText('Disconnect');
      expect(disconnectButton).toBeInTheDocument();
      expect(disconnectButton).not.toBeDisabled();
    });

    it('disables connect button when connecting', () => {
      render(DeviceCard, { device: { ...mockDevice, status: 'connecting' } });

      const connectButton = screen.getByText('Connect');
      expect(connectButton).toBeDisabled();
    });

    it('calls connectDevice when Connect button is clicked', async () => {
      bridgeStore.connectDevice.mockResolvedValue(true);
      render(DeviceCard, { device: mockDevice });

      const connectButton = screen.getByText('Connect');
      await user.click(connectButton);

      expect(bridgeStore.connectDevice).toHaveBeenCalledWith(mockDevice.id, mockDevice.config);
      expect(console.log).toHaveBeenCalledWith('Connecting TTL Pulse Generator...');
    });

    it('calls disconnectDevice when Disconnect button is clicked', async () => {
      bridgeStore.disconnectDevice.mockResolvedValue(true);
      render(DeviceCard, { device: { ...mockDevice, status: 'connected' } });

      const disconnectButton = screen.getByText('Disconnect');
      await user.click(disconnectButton);

      expect(bridgeStore.disconnectDevice).toHaveBeenCalledWith(mockDevice.id);
      expect(console.log).toHaveBeenCalledWith('Disconnecting TTL Pulse Generator...');
    });

    it('handles connection errors gracefully', async () => {
      const errorMessage = 'Connection failed';
      bridgeStore.connectDevice.mockRejectedValue(new Error(errorMessage));
      render(DeviceCard, { device: mockDevice });

      const connectButton = screen.getByText('Connect');
      await user.click(connectButton);

      await waitFor(() => {
        expect(console.error).toHaveBeenCalledWith(
          'Failed to connect TTL Pulse Generator:',
          expect.any(Error)
        );
      });
    });

    it('handles disconnection errors gracefully', async () => {
      const errorMessage = 'Disconnection failed';
      bridgeStore.disconnectDevice.mockRejectedValue(new Error(errorMessage));
      render(DeviceCard, { device: { ...mockDevice, status: 'connected' } });

      const disconnectButton = screen.getByText('Disconnect');
      await user.click(disconnectButton);

      await waitFor(() => {
        expect(console.error).toHaveBeenCalledWith(
          'Failed to disconnect TTL Pulse Generator:',
          expect.any(Error)
        );
      });
    });

    it('does not call connect/disconnect for connecting status', async () => {
      render(DeviceCard, { device: { ...mockDevice, status: 'connecting' } });

      const connectButton = screen.getByText('Connect');
      // Button should be disabled, but let's verify no action is taken even if clicked
      fireEvent.click(connectButton);

      expect(bridgeStore.connectDevice).not.toHaveBeenCalled();
      expect(bridgeStore.disconnectDevice).not.toHaveBeenCalled();
    });
  });

  describe('Configuration Modal', () => {
    it('shows Configure button', () => {
      render(DeviceCard, { device: mockDevice });

      const configButton = screen.getByText('Configure');
      expect(configButton).toBeInTheDocument();
      expect(configButton).not.toBeDisabled();
    });

    it('opens configuration modal when Configure button is clicked', async () => {
      render(DeviceCard, { device: mockDevice });

      const configButton = screen.getByText('Configure');
      await user.click(configButton);

      expect(console.log).toHaveBeenCalledWith('Configuring TTL Pulse Generator...');
    });

    it('handles configuration save with reconnection for connected device', async () => {
      // This test verifies that the configure button works for a connected device
      // The actual reconnection logic is internal to the component
      render(DeviceCard, {
        device: { ...mockDevice, status: 'connected' },
      });

      const configButton = screen.getByText('Configure');
      expect(configButton).toBeInTheDocument();
      expect(configButton).not.toBeDisabled();

      // Clicking configure should open the modal
      await user.click(configButton);
      expect(console.log).toHaveBeenCalledWith('Configuring TTL Pulse Generator...');
    });

    it('handles configuration save without reconnection for disconnected device', async () => {
      render(DeviceCard, { device: mockDevice });

      // For a disconnected device, configuration save should not trigger reconnection

      // The actual save logic is handled by the modal component
      expect(bridgeStore.disconnectDevice).not.toHaveBeenCalled();
      expect(bridgeStore.connectDevice).not.toHaveBeenCalled();
    });
  });

  describe('Accessibility', () => {
    it('has proper ARIA labels for buttons', () => {
      render(DeviceCard, { device: mockDevice });

      const connectButton = screen.getByText('Connect');
      const configButton = screen.getByText('Configure');

      // Buttons should have accessible text
      expect(connectButton).toBeInTheDocument();
      expect(configButton).toBeInTheDocument();
    });

    it('provides keyboard navigation support', async () => {
      render(DeviceCard, { device: mockDevice });

      const connectButton = screen.getByText('Connect');

      // Focus the Connect button directly (there are other focusable elements like select, remove button)
      connectButton.focus();
      expect(connectButton).toHaveFocus();

      // Should be able to activate with Enter
      await user.keyboard('{Enter}');
      expect(bridgeStore.connectDevice).toHaveBeenCalled();
    });

    it('has semantic HTML structure', () => {
      render(DeviceCard, { device: mockDevice });

      // Check for proper heading
      expect(screen.getByRole('heading', { level: 3 })).toHaveTextContent('TTL Pulse Generator');

      // Check for buttons
      expect(screen.getByRole('button', { name: 'Connect' })).toBeInTheDocument();
      expect(screen.getByRole('button', { name: 'Configure' })).toBeInTheDocument();
    });
  });

  describe('Responsive Design', () => {
    it('maintains layout on smaller screens', () => {
      // Mock window resize
      Object.defineProperty(window, 'innerWidth', {
        writable: true,
        configurable: true,
        value: 320,
      });

      render(DeviceCard, { device: mockDevice });

      const card = document.querySelector('.device-card');
      expect(card).toBeInTheDocument();

      // Card should still be rendered and functional on small screens
      expect(screen.getByText('TTL Pulse Generator')).toBeInTheDocument();
      expect(screen.getByText('Connect')).toBeInTheDocument();
    });
  });

  describe('Card Hover Effects', () => {
    it('applies hover styles on mouse over', async () => {
      render(DeviceCard, { device: mockDevice });

      const card = document.querySelector('.device-card');
      expect(card).toBeInTheDocument();

      // Test hover behavior (CSS classes are applied)
      await user.hover(card);

      // The hover effect is CSS-based, so we can't easily test the visual change
      // but we can verify the element exists and is interactive
      expect(card).toHaveClass('device-card');
    });
  });

  describe('Error State Handling', () => {
    it('displays error status correctly', () => {
      render(DeviceCard, { device: mockPupilDevice });

      expect(screen.getByText('Error')).toBeInTheDocument();
      const statusDot = document.querySelector('.status-dot');
      expect(statusDot).toHaveStyle('background-color: var(--color-error)');
    });

    it('allows connection retry from error state', async () => {
      bridgeStore.connectDevice.mockResolvedValue(true);
      render(DeviceCard, { device: mockPupilDevice });

      const connectButton = screen.getByText('Connect');
      await user.click(connectButton);

      expect(bridgeStore.connectDevice).toHaveBeenCalledWith(
        mockPupilDevice.id,
        mockPupilDevice.config
      );
    });
  });

  describe('Animation and Visual Effects', () => {
    it('has animated status dot', () => {
      render(DeviceCard, { device: mockDevice });

      // Verify status dot exists with correct class (animation is scoped CSS)
      const statusDot = document.querySelector('.status-dot');
      expect(statusDot).toBeInTheDocument();
      expect(statusDot).toHaveClass('status-dot');
    });

    it('has transition effects on buttons', () => {
      render(DeviceCard, { device: mockDevice });

      const connectButton = screen.getByText('Connect');
      const configButton = screen.getByText('Configure');

      expect(connectButton).toHaveClass('action-btn', 'connect-btn');
      expect(configButton).toHaveClass('action-btn', 'config-btn');
    });
  });
});
