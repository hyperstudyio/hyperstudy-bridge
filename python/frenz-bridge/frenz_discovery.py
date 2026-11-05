"""Credential-free Bluetooth discovery; never imports the Earable SDK."""
from collections import Counter


def select_bands(advertisements):
    bands = []
    for device, advert in advertisements.values():
        name = advert.local_name
        if name and name.upper().startswith('FRENZ'):
            bands.append({'name': name, 'address': device.address, 'rssi': advert.rssi})
    counts = Counter(band['name'] for band in bands)
    for band in bands:
        # The SDK accepts a name, not a BLE address. Do not guess between twins.
        band['selectable'] = counts[band['name']] == 1
    return sorted(bands, key=lambda band: (band['name'], band['address']))


def scan_error(error):
    text = str(error).lower()
    if 'turned off' in text or 'powered off' in text:
        return 'Turn on Bluetooth on this computer, then scan again.'
    if any(word in text for word in ('denied', 'unauthorized', 'not authorized', 'permission')):
        return 'Allow Bluetooth permission for HyperStudy Bridge in System Settings, then scan again.'
    return 'Bluetooth scan failed. Check Bluetooth access and that the band is advertising.'


async def scan():
    from bleak import BleakScanner
    return select_bands(await BleakScanner.discover(timeout=6., return_adv=True))
