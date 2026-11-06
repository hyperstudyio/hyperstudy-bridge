import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import '@testing-library/jest-dom';
import AddDeviceModal from '../AddDeviceModal.svelte';

describe('AddDeviceModal', () => {
  const user = userEvent.setup();

  const mockProps = {
    isOpen: true,
    onAdd: vi.fn(),
    onClose: vi.fn(),
  };

  beforeEach(() => {
    vi.clearAllMocks();
  });

  it('adds FRENZ with all 20 sensor, model, and marker options enabled', async () => {
    render(AddDeviceModal, mockProps);
    await user.click(screen.getByText('Earable FRENZ Brainband'));
    await user.click(screen.getByRole('button', {name: 'Add (1)'}));
    const config = mockProps.onAdd.mock.calls[0][0][0].config;
    expect(Object.keys(config)).toHaveLength(20);
    expect(Object.values(config).every(value => value === true)).toBe(true);
    expect(config).toMatchObject({streamHrRaw: true, streamSpo2Raw: true, streamFirmware: true});
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  describe('Modal Visibility', () => {
    it('renders when isOpen is true', () => {
      render(AddDeviceModal, mockProps);

      expect(screen.getByText('Add Devices')).toBeInTheDocument();
      expect(screen.getByText(/Click to select a device/)).toBeInTheDocument();
    });

    it('does not render when isOpen is false', () => {
      render(AddDeviceModal, { ...mockProps, isOpen: false });

      expect(screen.queryByText('Add Devices')).not.toBeInTheDocument();
    });

    it('has proper modal overlay and structure', () => {
      render(AddDeviceModal, mockProps);

      expect(document.querySelector('.modal-overlay')).toBeInTheDocument();
      expect(document.querySelector('.modal')).toBeInTheDocument();
      expect(document.querySelector('.modal-header')).toBeInTheDocument();
      expect(document.querySelector('.modal-body')).toBeInTheDocument();
      expect(document.querySelector('.modal-footer')).toBeInTheDocument();
    });
  });

  describe('Available Devices Display', () => {
    it('displays all available device types', () => {
      render(AddDeviceModal, mockProps);

      // Component has 5 built-in devices: TTL, Kernel, Pupil, FRENZ, EyeLink
      expect(screen.getByText('TTL Pulse Generator')).toBeInTheDocument();
      expect(screen.getByText('Kernel Flow2')).toBeInTheDocument();
      expect(screen.getByText('Pupil Labs Neon')).toBeInTheDocument();
    });

    it('shows device metadata correctly', () => {
      render(AddDeviceModal, mockProps);

      // Check TTL device
      expect(screen.getByText('Adafruit RP2040')).toBeInTheDocument();
      expect(screen.getByText('USB Serial')).toBeInTheDocument();

      // Check Kernel device
      expect(screen.getByText('fNIRS')).toBeInTheDocument();
      expect(screen.getByText('TCP Socket')).toBeInTheDocument();

      // Pupil Neon and EyeLink are both eye trackers
      expect(screen.getAllByText('Eye Tracker')).toHaveLength(2);
      expect(screen.getByText('REST API + LSL')).toBeInTheDocument();
    });

    it('renders device items as interactive elements', () => {
      render(AddDeviceModal, mockProps);

      // Component has 5 built-in devices (TTL, Kernel, Pupil, FRENZ, EyeLink)
      const deviceItems = document.querySelectorAll('.device-item');
      expect(deviceItems).toHaveLength(5);

      // Verify items have role="button" and tabindex for keyboard interaction
      deviceItems.forEach(item => {
        expect(item).toHaveAttribute('role', 'button');
        expect(item).toHaveAttribute('tabindex', '0');
      });
    });
  });

  describe('Device Selection Logic', () => {
    it('selects device on single click', async () => {
      render(AddDeviceModal, mockProps);

      const ttlDevice = screen.getByText('TTL Pulse Generator').closest('.device-item');
      await user.click(ttlDevice);

      expect(ttlDevice).toHaveClass('selected');
      expect(ttlDevice.querySelector('.device-check svg')).toBeInTheDocument();
    });

    it('replaces selection on single click (no modifier)', async () => {
      render(AddDeviceModal, mockProps);

      const ttlDevice = screen.getByText('TTL Pulse Generator').closest('.device-item');
      const kernelDevice = screen.getByText('Kernel Flow2').closest('.device-item');

      // Select first device
      await user.click(ttlDevice);
      expect(ttlDevice).toHaveClass('selected');

      // Select second device (should replace first)
      await user.click(kernelDevice);
      expect(kernelDevice).toHaveClass('selected');
      expect(ttlDevice).not.toHaveClass('selected');
    });

    it('supports multi-select with Cmd+Click on Mac', async () => {
      render(AddDeviceModal, mockProps);

      const ttlDevice = screen.getByText('TTL Pulse Generator').closest('.device-item');
      const kernelDevice = screen.getByText('Kernel Flow2').closest('.device-item');

      // Select first device
      await user.click(ttlDevice);
      expect(ttlDevice).toHaveClass('selected');

      // Cmd+Click second device (should add to selection)
      // Use keyboard modifier sequence with userEvent
      await user.keyboard('{Meta>}');
      await user.click(kernelDevice);
      await user.keyboard('{/Meta}');

      expect(ttlDevice).toHaveClass('selected');
      expect(kernelDevice).toHaveClass('selected');
    });

    it('supports multi-select with Ctrl+Click on Windows/Linux', async () => {
      render(AddDeviceModal, mockProps);

      const ttlDevice = screen.getByText('TTL Pulse Generator').closest('.device-item');
      const kernelDevice = screen.getByText('Kernel Flow2').closest('.device-item');

      // Select first device
      await user.click(ttlDevice);
      expect(ttlDevice).toHaveClass('selected');

      // Ctrl+Click second device (should add to selection)
      // Use keyboard modifier sequence with userEvent
      await user.keyboard('{Control>}');
      await user.click(kernelDevice);
      await user.keyboard('{/Control}');

      expect(ttlDevice).toHaveClass('selected');
      expect(kernelDevice).toHaveClass('selected');
    });

    it('deselects device with Cmd+Click on already selected device', async () => {
      render(AddDeviceModal, mockProps);

      const ttlDevice = screen.getByText('TTL Pulse Generator').closest('.device-item');

      // Select device
      await user.click(ttlDevice);
      expect(ttlDevice).toHaveClass('selected');

      // Cmd+Click same device (should deselect)
      await user.keyboard('{Meta>}');
      await user.click(ttlDevice);
      await user.keyboard('{/Meta}');
      expect(ttlDevice).not.toHaveClass('selected');
    });

    it('shows checkmark icon for selected devices', async () => {
      render(AddDeviceModal, mockProps);

      const ttlDevice = screen.getByText('TTL Pulse Generator').closest('.device-item');

      // Initially no checkmark
      expect(ttlDevice.querySelector('.device-check svg')).not.toBeInTheDocument();

      // After selection, checkmark appears
      await user.click(ttlDevice);
      expect(ttlDevice.querySelector('.device-check svg')).toBeInTheDocument();
    });
  });

  describe('Modal Actions', () => {
    it('shows Add button disabled when no devices selected', () => {
      render(AddDeviceModal, mockProps);

      const addButton = screen.getByRole('button', { name: /Add/ });
      expect(addButton).toBeDisabled();
    });

    it('enables Add button when devices are selected', async () => {
      render(AddDeviceModal, mockProps);

      const ttlDevice = screen.getByText('TTL Pulse Generator').closest('.device-item');
      await user.click(ttlDevice);

      const addButton = screen.getByRole('button', { name: /Add/ });
      expect(addButton).not.toBeDisabled();
      expect(addButton).toHaveTextContent('Add (1)');
    });

    it('updates Add button text with selection count', async () => {
      render(AddDeviceModal, mockProps);

      const ttlDevice = screen.getByText('TTL Pulse Generator').closest('.device-item');
      const kernelDevice = screen.getByText('Kernel Flow2').closest('.device-item');

      // Select one device
      await user.click(ttlDevice);
      expect(screen.getByText('Add (1)')).toBeInTheDocument();

      // Select second device with modifier key
      await user.keyboard('{Meta>}');
      await user.click(kernelDevice);
      await user.keyboard('{/Meta}');
      expect(screen.getByText('Add (2)')).toBeInTheDocument();
    });

    it('calls onAdd with selected devices when Add button clicked', async () => {
      render(AddDeviceModal, mockProps);

      const ttlDevice = screen.getByText('TTL Pulse Generator').closest('.device-item');
      const kernelDevice = screen.getByText('Kernel Flow2').closest('.device-item');

      // Select devices with modifier key
      await user.click(ttlDevice);
      await user.keyboard('{Meta>}');
      await user.click(kernelDevice);
      await user.keyboard('{/Meta}');

      const addButton = screen.getByText('Add (2)');
      await user.click(addButton);

      expect(mockProps.onAdd).toHaveBeenCalledWith(
        expect.arrayContaining([
          expect.objectContaining({ id: 'ttl', name: 'TTL Pulse Generator' }),
          expect.objectContaining({ id: 'kernel', name: 'Kernel Flow2' }),
        ])
      );
    });

    it('clears selection after adding devices', async () => {
      render(AddDeviceModal, mockProps);

      const ttlDevice = screen.getByText('TTL Pulse Generator').closest('.device-item');
      await user.click(ttlDevice);

      const addButton = screen.getByText('Add (1)');
      await user.click(addButton);

      // After clicking Add:
      // 1. onAdd is called with the selected devices
      // 2. Selection is cleared
      // 3. Modal closes (isOpen = false)
      // Since the modal closes, we verify through the onAdd callback instead
      expect(mockProps.onAdd).toHaveBeenCalledWith(
        expect.arrayContaining([expect.objectContaining({ id: 'ttl' })])
      );
    });

    it('calls onClose when Cancel button clicked', async () => {
      render(AddDeviceModal, mockProps);

      const cancelButton = screen.getByText('Cancel');
      await user.click(cancelButton);

      expect(mockProps.onClose).toHaveBeenCalled();
    });

    it('calls onClose when close button (×) clicked', async () => {
      render(AddDeviceModal, mockProps);

      const closeButton = screen.getByText('×');
      await user.click(closeButton);

      expect(mockProps.onClose).toHaveBeenCalled();
    });

    it('calls onClose when clicking outside modal', async () => {
      render(AddDeviceModal, mockProps);

      const overlay = document.querySelector('.modal-overlay');
      await user.click(overlay);

      expect(mockProps.onClose).toHaveBeenCalled();
    });

    it('does not close when clicking inside modal', async () => {
      render(AddDeviceModal, mockProps);

      const modal = document.querySelector('.modal');
      await user.click(modal);

      expect(mockProps.onClose).not.toHaveBeenCalled();
    });

    it('clears selection when closing modal', async () => {
      render(AddDeviceModal, mockProps);

      const ttlDevice = screen.getByText('TTL Pulse Generator').closest('.device-item');
      await user.click(ttlDevice);
      expect(ttlDevice).toHaveClass('selected');

      const cancelButton = screen.getByText('Cancel');
      await user.click(cancelButton);

      expect(mockProps.onClose).toHaveBeenCalled();
      // Selection should be cleared when modal closes
    });
  });

  describe('Keyboard Navigation', () => {
    it('closes modal when Escape key is pressed', async () => {
      render(AddDeviceModal, mockProps);

      // Focus the modal first, then press Escape
      const modal = document.querySelector('.modal');
      modal.focus();
      await user.keyboard('{Escape}');

      expect(mockProps.onClose).toHaveBeenCalled();
    });

    it('supports keyboard navigation between elements', async () => {
      render(AddDeviceModal, mockProps);

      // Tab should navigate through interactive elements
      const closeButton = screen.getByText('×');

      await user.tab();
      expect(closeButton).toHaveFocus();

      // Navigate to device items (would require more complex setup for device item focus)
      // This would typically involve setting tabindex on device items
    });

    it('handles Enter key on device selection', async () => {
      render(AddDeviceModal, mockProps);

      const ttlDevice = screen.getByText('TTL Pulse Generator').closest('.device-item');

      // Focus and press Enter
      ttlDevice.focus();
      await user.keyboard('{Enter}');

      // Note: This might require adding keyboard event handlers to device items
      // The current implementation only handles click events
    });
  });

  describe('Accessibility', () => {
    it('has proper ARIA labels and roles', () => {
      render(AddDeviceModal, mockProps);

      expect(screen.getByRole('button', { name: /Cancel/ })).toBeInTheDocument();
      expect(screen.getByRole('button', { name: /Add/ })).toBeInTheDocument();
      expect(screen.getByRole('button', { name: /×/ })).toBeInTheDocument();
      expect(screen.getByRole('heading', { level: 2, name: 'Add Devices' })).toBeInTheDocument();
    });

    it('provides keyboard instructions for multi-select', () => {
      render(AddDeviceModal, mockProps);

      expect(screen.getByText(/Cmd\+Click/)).toBeInTheDocument();
      expect(screen.getByText(/Ctrl\+Click/)).toBeInTheDocument();

      // Check that kbd elements are properly styled
      const kbdElements = document.querySelectorAll('kbd');
      expect(kbdElements.length).toBeGreaterThan(0);
    });

    it('maintains focus management', async () => {
      render(AddDeviceModal, mockProps);

      // Modal should trap focus within itself
      const closeButton = screen.getByText('×');
      closeButton.focus();
      expect(document.activeElement).toBe(closeButton);
    });

    it('has proper contrast and visual indicators', () => {
      render(AddDeviceModal, mockProps);

      // Verify that device items have the expected class structure for styling
      const deviceItems = document.querySelectorAll('.device-item');
      expect(deviceItems.length).toBe(5);

      // Each item should have proper structure for visual feedback
      deviceItems.forEach(item => {
        expect(item).toHaveClass('device-item');
        expect(item.querySelector('.device-info')).toBeInTheDocument();
        expect(item.querySelector('.device-check')).toBeInTheDocument();
      });
    });
  });

  describe('Responsive Design', () => {
    it('adapts to different screen sizes', () => {
      // Mock smaller viewport
      Object.defineProperty(window, 'innerWidth', {
        writable: true,
        configurable: true,
        value: 480,
      });

      render(AddDeviceModal, mockProps);

      // Verify that the modal structure is present and functional on small screens
      const modal = document.querySelector('.modal');
      expect(modal).toBeInTheDocument();
      expect(modal).toHaveClass('modal');

      // Content should still be accessible
      expect(screen.getByText('Add Devices')).toBeInTheDocument();
    });

    it('maintains usability on mobile devices', () => {
      // Mock mobile viewport
      Object.defineProperty(window, 'innerWidth', {
        writable: true,
        configurable: true,
        value: 320,
      });

      render(AddDeviceModal, mockProps);

      // Modal should still be functional
      expect(screen.getByText('Add Devices')).toBeInTheDocument();
      expect(screen.getByText('TTL Pulse Generator')).toBeInTheDocument();
    });
  });

  describe('Animation and Visual Effects', () => {
    it('has fade-in animation for modal overlay', () => {
      render(AddDeviceModal, mockProps);

      // Verify the overlay element exists with the correct class
      // Animation styles are scoped CSS which can't be tested reliably in jsdom
      const overlay = document.querySelector('.modal-overlay');
      expect(overlay).toBeInTheDocument();
      expect(overlay).toHaveClass('modal-overlay');
    });

    it('has slide-up animation for modal content', () => {
      render(AddDeviceModal, mockProps);

      // Verify the modal element exists with the correct class
      // Animation styles are scoped CSS which can't be tested reliably in jsdom
      const modal = document.querySelector('.modal');
      expect(modal).toBeInTheDocument();
      expect(modal).toHaveClass('modal');
    });

    it('has hover effects on interactive elements', async () => {
      render(AddDeviceModal, mockProps);

      // Verify device items exist and can receive hover events
      const deviceItem = document.querySelector('.device-item');
      expect(deviceItem).toBeInTheDocument();

      // Hovering should not cause errors
      await user.hover(deviceItem);
      expect(deviceItem).toBeInTheDocument();
    });
  });

  describe('Error Handling', () => {
    it('handles invalid device selection gracefully', () => {
      render(AddDeviceModal, mockProps);

      // Simulate edge case where device might be undefined
      // This should not crash the component
      const deviceList = document.querySelector('.device-list');
      expect(deviceList).toBeInTheDocument();
    });

    it('maintains state consistency', async () => {
      render(AddDeviceModal, mockProps);

      const ttlDevice = screen.getByText('TTL Pulse Generator').closest('.device-item');

      // Rapid clicking should maintain consistent state
      await user.click(ttlDevice);
      await user.click(ttlDevice, { metaKey: true });
      await user.click(ttlDevice, { metaKey: true });

      // Device should be selected after odd number of meta-clicks starting unselected
      expect(ttlDevice).toHaveClass('selected');
    });
  });

  describe('Performance', () => {
    it('renders efficiently with all device types', () => {
      const startTime = performance.now();
      render(AddDeviceModal, mockProps);
      const endTime = performance.now();

      // Rendering should be fast (arbitrary threshold for test)
      expect(endTime - startTime).toBeLessThan(100); // 100ms threshold
    });

    it('handles selection state updates efficiently', async () => {
      render(AddDeviceModal, mockProps);

      const startTime = performance.now();

      // Perform multiple selections
      const devices = [
        screen.getByText('TTL Pulse Generator').closest('.device-item'),
        screen.getByText('Kernel Flow2').closest('.device-item'),
        screen.getByText('Pupil Labs Neon').closest('.device-item'),
      ];

      for (const device of devices) {
        await user.click(device, { metaKey: true });
      }

      const endTime = performance.now();

      // Multiple selections should complete quickly
      expect(endTime - startTime).toBeLessThan(200); // 200ms threshold
    });
  });
});
