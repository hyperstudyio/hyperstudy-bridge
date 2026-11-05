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
from frenz_streams import STREAMS, StreamPublisher

shutdown_event = threading.Event()
protocol_output = sys.stdout


def emit(status):
    protocol_output.write(json.dumps(status, separators=(',', ':')) + '\n')
    protocol_output.flush()


def stdin_listener():
    for line in sys.stdin:
        if line.strip().lower() == 'stop':
            break
    shutdown_event.set()


def create_outlets(device_id, session_id, wall_to_lsl):
    import pylsl
    outlets = {}
    for suffix, store, key, channels, rate, fmt, policy in STREAMS:
        info = pylsl.StreamInfo(device_id + suffix, 'Markers' if fmt == 'string' else 'Biosignals',
                               channels, rate, fmt, f'frenz-{device_id}-{session_id}{suffix}')
        desc = info.desc()
        for name, value in {'schema_version':'2', 'manufacturer':'Earable', 'device_id':device_id,
                            'sdk_version':'0.3.2', 'sdk_store':store, 'sdk_key':key,
                            'timestamp_policy':policy, 'session_id':session_id,
                            'source_unix_to_lsl_offset':repr(wall_to_lsl),
                            'source_timestamp_column':'0' if store in ('raw','firmware') else 'none',
                            'sample_order':'SDK array index; never timestamp deduplication'}.items():
            desc.append_child_value(name, value)
        channel_metadata = desc.append_child('channels')
        for index in range(channels):
            channel = channel_metadata.append_child('channel')
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
    failed = False
    try:
        config = json.loads(sys.stdin.readline())
        device_id, product_key = config['device_id'], config['product_key']
        if not isinstance(device_id, str) or not device_id.strip() or not isinstance(product_key,str) or not product_key:
            raise ValueError('Device ID and product key are required')
        config.clear()
        threading.Thread(target=stdin_listener, daemon=True).start()
        emit({'status':'bootstrapping','phase':'importing','package':'frenztoolkit'})
        from frenztoolkit import Streamer
        import pylsl
        # SDK uses the process cwd after construction. Its recording leaf must
        # be relative and absent; a precreated absolute directory is rejected.
        root = Path(tempfile.mkdtemp(prefix='hyperstudy-frenz-'))
        os.chdir(root)
        emit({'status':'connecting','phase':'sdk_start','device_id':device_id})
        streamer = Streamer(device_id=device_id, product_key=product_key,
                            data_folder='vendor-native', turn_off_light=False)
        product_key = None
        session_id = str(uuid.uuid4())
        wall_to_lsl = pylsl.local_clock() - time.time()
        outlets = create_outlets(device_id, session_id, wall_to_lsl)
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

        while not shutdown_event.is_set():
            publisher.poll(streamer, publish, pylsl.local_clock())
            now = time.monotonic()
            if publisher.sample_count != previous_count:
                last_sample = now
                previous_count = publisher.sample_count
            has_eeg = publisher.cursors.get('_EEG_raw', 0) > 0
            if not streaming and has_eeg:
                streaming = True
                emit({'status':'streaming','streams':list(outlets), 'sample_count':publisher.sample_count})
            if (not streaming and now - started > 30) or (streaming and now - last_sample > 15):
                raise TimeoutError('No new FRENZ samples received')
            if streaming and now - last_report >= 5:
                emit({'status':'streaming','streams':list(outlets),'sample_count':publisher.sample_count,
                      'stream_sample_counts':publisher.cursors})
                last_report = now
            # Polling cadence for an SDK without a callback API, not a startup delay.
            shutdown_event.wait(.02)
    except BaseException as error:
        failed = True
        emit({'status':'error','message':f'FRENZ acquisition failed ({type(error).__name__})'})
    finally:
        if streamer is not None:
            # Vendor stop may terminate the process; report stopping first.
            emit({'status':'stopping'})
            try:
                streamer.stop()
            except BaseException:
                failed = True
                emit({'status':'error','message':'FRENZ shutdown failed'})
        if not failed:
            emit({'status':'stopped'})
    if failed:
        raise SystemExit(1)


if __name__ == '__main__':
    main()
