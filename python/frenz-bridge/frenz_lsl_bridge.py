"""Credential-safe FRENZ sidecar: stdin config/stop, stdout JSON status."""
import json
import os
from pathlib import Path
import signal
import sys
import tempfile
import threading
import time
import uuid
from frenz_streams import STREAMS, StreamPublisher, measure_clock_pair, timing_metadata
from frenz_bluetooth import EEG_COLUMNS, EEG_SUFFIX, EegPacketTap

shutdown_event = threading.Event()
protocol_output = sys.stdout


class StreamResourceError(RuntimeError):
    """Fixed, credential-safe diagnostics for local runtime resource limits."""


def prepare_lsl_runtime():
    # Finder/LaunchServices children inherit a 256-descriptor soft limit on
    # macOS. Each LSL outlet owns multiple sockets; all FRENZ streams exceed it.
    # Raise only this process's soft limit, never the system or hard limit.
    if sys.platform == 'win32':
        return
    import resource
    soft, hard = resource.getrlimit(resource.RLIMIT_NOFILE)
    required = 1024
    if soft == resource.RLIM_INFINITY or soft >= required:
        return
    if hard != resource.RLIM_INFINITY and hard < required:
        raise StreamResourceError('FRENZ requires a file/socket limit of at least 1024.')
    try:
        resource.setrlimit(resource.RLIMIT_NOFILE, (required, hard))
    except (ValueError, OSError) as error:
        raise StreamResourceError('Unable to reserve file/socket capacity for FRENZ streams.') from error


def emit(status):
    protocol_output.write(json.dumps(status, separators=(',', ':')) + '\n')
    protocol_output.flush()


def stdin_listener():
    for line in sys.stdin:
        if line.strip().lower() == 'stop':
            break
    shutdown_event.set()


def create_outlets(device_id, session_id, clock_pair):
    import pylsl
    outlets = {}
    for suffix, store, key, channels, rate, fmt, policy in STREAMS:
        info = pylsl.StreamInfo(device_id + suffix, 'Markers' if fmt == 'string' else 'Biosignals',
                               channels, rate, fmt, f'frenz-{device_id}-{session_id}{suffix}')
        desc = info.desc()
        for name, value in {'schema_version':'4' if store == 'bluetooth' else '3', 'manufacturer':'Earable', 'device_id':device_id,
                            'sdk_version':'0.3.2', 'sdk_store':store, 'sdk_key':key,
                            'timestamp_policy':policy, 'session_id':session_id,
                            **timing_metadata(store, policy, clock_pair),
                            'source_timestamp_column':'0' if store in ('raw','firmware','bluetooth') else 'none',
                            'sample_order':('packet_index then row_in_packet; no deduplication' if store == 'bluetooth'
                                            else 'SDK array index; never timestamp deduplication')}.items():
            desc.append_child_value(name, value)
        channel_metadata = desc.append_child('channels')
        for index in range(channels):
            channel = channel_metadata.append_child('channel')
            if store == 'bluetooth':
                label, unit = EEG_COLUMNS[index]
                channel.append_child_value('label', label)
                channel.append_child_value('unit', unit)
                continue
            source_timestamp = index == 0 and store in ('raw', 'firmware')
            channel.append_child_value('label', 'source_timestamp' if source_timestamp else f'sdk_column_{index}')
            channel.append_child_value('sdk_column_index', str(index))
            # Preserve column order without inventing electrode names or units.
            channel.append_child_value('unit', 'seconds' if source_timestamp else 'unspecified')
        outlets[suffix] = pylsl.StreamOutlet(info, max_buffered=360)
    return outlets


def main():
    global protocol_output
    # Preserve the protocol descriptor, then suppress arbitrary vendor output.
    # SDK diagnostics may contain credentials; only our structured statuses leave.
    protocol_output = os.fdopen(os.dup(sys.stdout.fileno()), 'w', buffering=1)
    with open(os.devnull, 'w') as null:
        os.dup2(null.fileno(), sys.stdout.fileno())
        os.dup2(null.fileno(), sys.stderr.fileno())
    for sig in (signal.SIGTERM, signal.SIGINT):
        signal.signal(sig, lambda *_: shutdown_event.set())
    emit({'status':'waiting_for_config'})
    streamer = None
    streamer_start_attempted = False
    packet_tap = None
    failed = False
    phase = "configuration"
    try:
        config = json.loads(sys.stdin.readline())
        if config.get('action') == 'scan':
            import asyncio
            from frenz_discovery import scan, scan_error
            try:
                emit({'status': 'discovered', 'devices': asyncio.run(scan())})
            except Exception as error:
                emit({'status': 'scan_error', 'message': scan_error(error)})
            return
        device_id, product_key = config['device_id'], config['product_key']
        if not isinstance(device_id, str) or not device_id.strip() or not isinstance(product_key,str) or not product_key:
            raise ValueError('Device ID and product key are required')
        config.clear()
        threading.Thread(target=stdin_listener, daemon=True).start()
        phase = "preparing_runtime"
        prepare_lsl_runtime()
        emit({'status':'bootstrapping','phase':'importing','package':'frenztoolkit'})
        phase = "loading_sdk"
        from frenztoolkit import Streamer
        import pylsl
        # SDK uses the process cwd after construction. Its recording leaf must
        # be relative and absent; a precreated absolute directory is rejected.
        root = Path(tempfile.mkdtemp(prefix='hyperstudy-frenz-'))
        os.chdir(root)
        emit({'status':'connecting','phase':'sdk_start','device_id':device_id})
        phase = "sdk_initialization"
        streamer = Streamer(device_id=device_id, product_key=product_key,
                            data_folder='vendor-native', turn_off_light=False)
        product_key = None
        session_id = str(uuid.uuid4())
        clock_pair = measure_clock_pair(pylsl.local_clock, time.time)
        wall_to_lsl = clock_pair['offset_seconds']
        phase = "creating_streams"
        outlets = create_outlets(device_id, session_id, clock_pair)
        packet_tap = EegPacketTap(pylsl.local_clock)
        phase = "installing_packet_capture"
        packet_tap.install()
        phase = "connecting_bluetooth"
        streamer_start_attempted = True
        streamer.start()
        publisher = StreamPublisher()
        started = time.monotonic()
        last_report = started
        last_sample = started
        previous_count = 0
        streaming = False
        emit({'status':'connecting','phase':'waiting_for_data','device_id':device_id})

        def publish(suffix, rows, observed, policy):
            values = rows if isinstance(rows, list) else rows.tolist()
            timestamps = ([float(row[0]) + wall_to_lsl for row in values]
                          if policy == 'source_unix' else [observed] * len(values))
            outlets[suffix].push_chunk(values, timestamp=timestamps)

        def counts():
            return {**publisher.cursors, EEG_SUFFIX: packet_tap.sample_count}

        def timing():
            return {**publisher.timing_status(), EEG_SUFFIX: packet_tap.timing_status()}

        while not shutdown_event.is_set():
            phase = "publishing_samples"
            packet_tap.drain(publish)
            publisher.poll(streamer, publish, pylsl.local_clock())
            now = time.monotonic()
            if packet_tap.sample_count != previous_count:
                last_sample = now
                previous_count = packet_tap.sample_count
            total_count = publisher.sample_count + packet_tap.sample_count
            has_eeg = packet_tap.sample_count > 0
            if not streaming and has_eeg:
                streaming = True
                emit({'status':'streaming','streams':list(outlets), 'sample_count':total_count,
                      'stream_sample_counts':counts(), 'timing':timing(), 'clock_pair':clock_pair})
            if (not streaming and now - started > 30) or (streaming and now - last_sample > 15):
                raise TimeoutError('No new FRENZ samples received')
            if streaming and now - last_report >= 5:
                emit({'status':'streaming','streams':list(outlets),'sample_count':total_count,
                      'stream_sample_counts':counts(), 'timing':timing(), 'clock_pair':clock_pair})
                last_report = now
            # Polling cadence for an SDK without a callback API, not a startup delay.
            shutdown_event.wait(.02)
        # Publish all packets already observed when stopping; do not silently
        # discard the queue just because a stop command arrived between polls.
        packet_tap.finish_and_drain(publish)
        publisher.poll(streamer, publish, pylsl.local_clock())
    except BaseException as error:
        failed = True
        # Allowlist a known SDK error; never expose arbitrary exception text.
        reason = ('The Earable SDK rejected the product key.'
                  if 'invalid product key' in str(error).lower()
                  else f'FRENZ failed during {phase} ({type(error).__name__}).')
        if packet_tap is not None and packet_tap.failure is not None:
            reason = packet_tap.failure  # Fixed allowlisted capture error, no vendor text.
        if isinstance(error, StreamResourceError):
            reason = str(error)
        emit({'status':'error', 'phase':phase, 'message':reason})
    finally:
        if packet_tap is not None:
            packet_tap.finish_capture()
        if streamer is not None and streamer_start_attempted:
            # Vendor stop may terminate the process; report stopping first.
            emit({'status':'stopping'})
            try:
                streamer.stop()
            except BaseException:
                failed = True
                emit({'status':'error','message':'FRENZ shutdown failed'})
        if packet_tap is not None:
            packet_tap.restore()
        if not failed:
            emit({'status':'stopped'})
    if failed:
        raise SystemExit(1)


if __name__ == '__main__':
    main()
