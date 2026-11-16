/**
 * The devices Bridge offers in Add Device, with their default settings. Shared with the code that restores the
 * devices a user added (selectedDevices.js), so a restored device always matches what this version offers.
 */
export const DEVICE_CATALOG = Object.freeze([
  {
    id: 'ttl',
    name: 'TTL Pulse Generator',
    type: 'Adafruit RP2040',
    connection: 'USB Serial',
    config: { port: '/dev/cu.usbmodem101' }, // Fallback default
  },
  {
    id: 'kernel',
    name: 'Kernel Flow2',
    type: 'fNIRS',
    connection: 'TCP Socket',
    config: { ip: '127.0.0.1', port: 6767 },
  },
  {
    id: 'pupil',
    name: 'Pupil Labs Neon',
    type: 'Eye Tracker',
    connection: 'REST API + LSL',
    config: { url: 'neon.local:8080' },
  },
  {
    id: 'frenz',
    name: 'Earable FRENZ Brainband',
    type: 'EEG/Biosignals',
    connection: 'LSL (via Python)',
    config: {
      streamEegRaw: true,
      streamHrRaw: true,
      streamSpo2Raw: true,
      streamFirmware: true,
      streamEegFiltered: true,
      streamEogFiltered: true,
      streamEmgFiltered: true,
      streamPpgRaw: true,
      streamImuRaw: true,
      streamFocus: true,
      streamSleepStage: true,
      streamPoas: true,
      streamPosture: true,
      streamSignalQuality: true,
      streamAlpha: true,
      streamBeta: true,
      streamTheta: true,
      streamGamma: true,
      streamDelta: true,
      enableMarkerOutlet: true,
    },
  },
]);
