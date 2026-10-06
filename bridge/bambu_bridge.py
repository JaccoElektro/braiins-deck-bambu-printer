#!/usr/bin/env python3
"""bambu-bridge — a read-only bridge between a Bambu Lab printer and the
Braiins Deck "Bambu Lab Printer" widget.

* Talks to the printer on your local network only (MQTT over TLS on 8883 and
  the P1/A1 camera on 6000). The access code stays on this computer; the Deck
  never sees it.
* Gives the Deck status only, never control:
    GET /bambu.json[?lang=en|nl]   normalised status (print, temperatures, AMS, errors)
    GET /camera.jpg                latest camera picture
* Only the addresses in `allowed_clients` (your Deck) and this computer may connect.
* Optional notifications when a print finishes, fails or pauses.

Configuration: ~/.config/bambu-bridge/config.json (see config.example.json),
or a path in the BAMBU_BRIDGE_CONFIG environment variable.
"""
import json
import os
import socket
import ssl
import struct
import subprocess
import sys
import threading
import time
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import paho.mqtt.client as mqtt

CONFIG_PATH = os.environ.get('BAMBU_BRIDGE_CONFIG', os.path.expanduser('~/.config/bambu-bridge/config.json'))


def load_config():
    try:
        with open(CONFIG_PATH) as f:
            cfg = json.load(f)
    except FileNotFoundError:
        sys.exit(f'No configuration at {CONFIG_PATH}; copy config.example.json there and fill it in.')
    for key in ('printer_host', 'serial'):
        if not cfg.get(key):
            sys.exit(f'{CONFIG_PATH}: "{key}" is required.')
    if os.name == 'posix' and os.stat(CONFIG_PATH).st_mode & 0o077:
        print(f'warning: {CONFIG_PATH} is readable by others; run: chmod 600 {CONFIG_PATH}', flush=True)
    return cfg


CFG = load_config()
PRINTER = CFG['printer_host']
SERIAL = CFG['serial']
LISTEN = ('0.0.0.0', int(CFG.get('listen_port', 9152)))
ALLOWED = {'127.0.0.1', '::1', *CFG.get('allowed_clients', [])}
NOTIFY = CFG.get('notify') or {}
LANG = CFG.get('language', 'en')

CAMERA_ACTIVE_S = 10    # picture interval while printing
CAMERA_WATCHED_S = 15   # …while the Deck shows the printer
CAMERA_IDLE_S = 300     # …otherwise
STATE = {}              # merged `print` object from MQTT reports
STATE_LOCK = threading.Lock()
LAST_REPORT = [0.0]
CAMERA = {'jpeg': None, 'at': 0.0}
WANTED = [0.0]          # last time someone asked for /camera.jpg


def access_code():
    """From the config, or from Bambu Studio's own settings on this computer."""
    if CFG.get('access_code'):
        return str(CFG['access_code'])
    for path in ('~/.config/BambuStudio/BambuStudio.conf',
                 '~/Library/Application Support/BambuStudio/BambuStudio.conf',
                 '~/AppData/Roaming/BambuStudio/BambuStudio.conf'):
        try:
            with open(os.path.expanduser(path)) as f:
                code = json.load(f).get('access_code', {}).get(SERIAL)
            if code:
                return code
        except (OSError, ValueError):
            continue
    sys.exit('No access_code in the config and none found in Bambu Studio; '
             'find it on the printer (Settings → Network / LAN) and add it to the config.')


def log(*a):
    print(time.strftime('%H:%M:%S'), *a, flush=True)


# ── Wording ─────────────────────────────────────────────────────────────

STATES = {
    'en': {'IDLE': 'Ready', 'PREPARE': 'Preparing', 'RUNNING': 'Printing', 'PAUSE': 'Paused',
           'FINISH': 'Finished', 'FAILED': 'Failed', 'SLICING': 'Slicing'},
    'nl': {'IDLE': 'Klaar voor gebruik', 'PREPARE': 'Voorbereiden', 'RUNNING': 'Printen', 'PAUSE': 'Gepauzeerd',
           'FINISH': 'Klaar', 'FAILED': 'Mislukt', 'SLICING': 'Slicen'},
}
STAGES = {
    'en': {1: 'Levelling bed', 2: 'Heating bed', 3: 'Vibration compensation', 4: 'Changing filament',
           6: 'Filament ran out', 7: 'Heating nozzle', 8: 'Calibrating extrusion', 9: 'Scanning bed',
           10: 'Checking first layer', 11: 'Identifying build plate', 13: 'Homing toolhead',
           14: 'Cleaning nozzle', 16: 'Paused by user', 17: 'Front cover open', 19: 'Calibrating flow',
           20: 'Nozzle temperature error', 21: 'Bed temperature error', 22: 'Unloading filament',
           24: 'Loading filament', 26: 'AMS lost', 29: 'Cooling chamber', 32: 'Nozzle clogged with filament',
           34: 'First layer defect', 35: 'Nozzle clogged'},
    'nl': {1: 'Bed nivelleren', 2: 'Bed opwarmen', 3: 'Trillingscompensatie', 4: 'Filament wisselen',
           6: 'Filament op', 7: 'Nozzle opwarmen', 8: 'Extrusie kalibreren', 9: 'Bed scannen',
           10: 'Eerste laag controleren', 11: 'Printplaat herkennen', 13: 'Printkop homen',
           14: 'Nozzle reinigen', 16: 'Gepauzeerd door gebruiker', 17: 'Voorklep open', 19: 'Flow kalibreren',
           20: 'Nozzle-temperatuurfout', 21: 'Bed-temperatuurfout', 22: 'Filament uitladen',
           24: 'Filament laden', 26: 'AMS kwijt', 29: 'Kamer afkoelen', 32: 'Nozzle bedekt met filament',
           34: 'Fout in eerste laag', 35: 'Nozzle verstopt'},
}
SPEEDS = {'en': {1: 'Silent', 2: 'Standard', 3: 'Sport', 4: 'Ludicrous'},
          'nl': {1: 'Stil', 2: 'Standaard', 3: 'Sport', 4: 'Ludicrous'}}
NOZZLES = {'en': {'hardened_steel': 'hardened steel', 'stainless_steel': 'stainless steel'},
           'nl': {'hardened_steel': 'gehard staal', 'stainless_steel': 'rvs'}}
MESSAGES = {
    'en': {'FINISH': '✅ Print finished: {job}. Take it off the plate!',
           'FAILED': '❌ Print failed: {job}. Have a look at the printer.',
           'PAUSE': '⏸️ Print paused: {job} (filament out or a warning?).',
           'job': 'your print'},
    'nl': {'FINISH': '✅ Print klaar: {job}. Haal hem van de plaat!',
           'FAILED': '❌ Print mislukt: {job}. Kijk even bij de printer.',
           'PAUSE': '⏸️ Print gepauzeerd: {job} (filament op of een melding?).',
           'job': 'je print'},
}


# ── Notifications (optional) ─────────────────────────────────────────────

def notify(msg):
    """POST the text to `notify.url` (ntfy, a Home Assistant webhook, …) and/or run `notify.command`."""
    def send():
        if NOTIFY.get('url'):
            try:
                req = urllib.request.Request(NOTIFY['url'], data=msg.encode(), method='POST',
                                             headers={'Content-Type': 'text/plain; charset=utf-8', **NOTIFY.get('headers', {})})
                urllib.request.urlopen(req, timeout=20).read()
            except OSError as e:
                log('notify url failed', e)
        if NOTIFY.get('command'):
            try:
                subprocess.run([*NOTIFY['command'], msg], timeout=30, check=False)
            except OSError as e:
                log('notify command failed', e)
    if NOTIFY:
        threading.Thread(target=send, daemon=True).start()


LAST_STATE = [None]


def notify_transition():
    """On print end or trouble. The first report only seeds the state."""
    words = MESSAGES.get(LANG, MESSAGES['en'])
    state, job = STATE.get('gcode_state'), STATE.get('subtask_name') or words['job']
    before, LAST_STATE[0] = LAST_STATE[0], state
    if before is None or before == state:
        return
    if (state == 'FINISH' and before in ('RUNNING', 'PAUSE')) or state in ('FAILED', 'PAUSE'):
        msg = words[state].format(job=job)
        log('notify:', msg)
        notify(msg)


# ── MQTT ────────────────────────────────────────────────────────────────

def on_connect(client, _u, _f, rc, _p=None):
    log('mqtt connected', rc)
    client.subscribe(f'device/{SERIAL}/report')
    request_full(client)


def request_full(client):
    client.publish(f'device/{SERIAL}/request', json.dumps({'pushing': {'sequence_id': '0', 'command': 'pushall'}}))


def on_message(_c, _u, msg):
    try:
        data = json.loads(msg.payload)
    except ValueError:
        return
    report = data.get('print')
    if not isinstance(report, dict):
        return
    with STATE_LOCK:
        # P1/A1 printers send deltas; merge them onto the last full picture.
        for k, v in report.items():
            if k == 'ams' and isinstance(v, dict) and isinstance(STATE.get('ams'), dict):
                STATE['ams'].update(v)
            else:
                STATE[k] = v
    LAST_REPORT[0] = time.time()
    notify_transition()


def mqtt_loop():
    client = mqtt.Client(mqtt.CallbackAPIVersion.VERSION2)
    client.username_pw_set('bblp', access_code())
    client.tls_set(cert_reqs=ssl.CERT_NONE)  # the printer uses a self-signed Bambu CA
    client.tls_insecure_set(True)
    client.on_connect = on_connect
    client.on_message = on_message
    client.reconnect_delay_set(5, 60)
    while True:
        try:
            client.connect(PRINTER, 8883, 30)
            client.loop_start()
            # P1 firmware only pushes changes; ask for a full report now and then.
            while True:
                time.sleep(300)
                request_full(client)
        except OSError as e:
            log('mqtt error', e)
            time.sleep(30)


# ── Camera (P1/A1 series: TLS on :6000, 80-byte auth, then framed JPEGs) ─

def grab_jpeg():
    auth = struct.pack('<IIII', 0x40, 0x3000, 0, 0)
    auth += b'bblp'.ljust(32, b'\0') + access_code().encode().ljust(32, b'\0')
    ctx = ssl.create_default_context()
    ctx.check_hostname = False
    ctx.verify_mode = ssl.CERT_NONE
    with socket.create_connection((PRINTER, 6000), timeout=10) as raw, ctx.wrap_socket(raw) as s:
        s.sendall(auth)
        header = b''
        while len(header) < 16:
            chunk = s.recv(16 - len(header))
            if not chunk:
                return None
            header += chunk
        size = struct.unpack('<I', header[:4])[0]
        if not 0 < size < 5_000_000:
            return None
        img = b''
        while len(img) < size:
            chunk = s.recv(min(65536, size - len(img)))
            if not chunk:
                return None
            img += chunk
    return img if img[:2] == b'\xff\xd8' else None


def camera_loop():
    if not CFG.get('camera', True):
        return
    while True:
        try:
            jpeg = grab_jpeg()
            if jpeg:
                CAMERA['jpeg'], CAMERA['at'] = jpeg, time.time()
        except OSError as e:
            log('camera error', e)
        # Fresh pictures while printing or while someone looks; rarely otherwise.
        while True:
            printing = STATE.get('gcode_state') in ('RUNNING', 'PREPARE', 'PAUSE')
            watched = time.time() - WANTED[0] < 60
            every = CAMERA_ACTIVE_S if printing else CAMERA_WATCHED_S if watched else CAMERA_IDLE_S
            if time.time() - CAMERA['at'] >= every:
                break
            time.sleep(1)


# ── Normalisation ───────────────────────────────────────────────────────

def num(v, default=0.0):
    try:
        return float(v)
    except (TypeError, ValueError):
        return default


def tray(t, slot, active):
    color = (t.get('tray_color') or '')[:6]
    return {
        'slot': slot,
        'type': t.get('tray_type') or '',
        'name': t.get('tray_sub_brands') or t.get('tray_type') or '',
        'color': color,
        'remain': int(t['remain']) if str(t.get('remain', '')).lstrip('-').isdigit() and int(t['remain']) >= 0 else None,
        'empty': not t.get('tray_type'),
        'active': active,
    }


def normalised(lang):
    lang = lang if lang in STATES else 'en'
    with STATE_LOCK:
        p = json.loads(json.dumps(STATE))
    online = time.time() - LAST_REPORT[0] < 900 and bool(p)
    gstate = p.get('gcode_state', '')
    stage = int(num(p.get('stg_cur'), -1))
    ams_root = p.get('ams') or {}
    tray_now = str(ams_root.get('tray_now', '255'))
    units = []
    for unit in ams_root.get('ams', []) or []:
        uid = int(num(unit.get('id'), 0))
        trays = [tray(t, uid * 4 + int(num(t.get('id'), 0)) + 1,
                      tray_now == str(uid * 4 + int(num(t.get('id'), 0))))
                 for t in unit.get('tray', [])]
        units.append({
            'id': uid,
            'humidity': num(unit.get('humidity_raw'), None) if unit.get('humidity_raw') is not None else None,
            'humidity_level': int(num(unit.get('humidity'), 0)),
            'temp': num(unit.get('temp'), None) if unit.get('temp') is not None else None,
            'trays': trays,
        })
    ext = p.get('vt_tray') or {}
    external = tray(ext, 0, tray_now == '254') if ext.get('tray_type') else None
    remaining = int(num(p.get('mc_remaining_time'), 0))
    lights = {l.get('node'): l.get('mode') for l in p.get('lights_report', []) or []}
    hms = p.get('hms') or []
    return {
        'online': online,
        'updated': int(LAST_REPORT[0]),
        'state': gstate,
        'state_label': STATES[lang].get(gstate, gstate.title() or '?'),
        'stage_label': STAGES[lang].get(stage, ''),
        'job': p.get('subtask_name') or '',
        'percent': int(num(p.get('mc_percent'), 0)),
        'remaining_min': remaining,
        'eta': int(time.time() + remaining * 60) if gstate == 'RUNNING' and remaining else None,
        'layer': int(num(p.get('layer_num'), 0)),
        'layers': int(num(p.get('total_layer_num'), 0)),
        'nozzle': round(num(p.get('nozzle_temper')), 1),
        'nozzle_target': round(num(p.get('nozzle_target_temper')), 1),
        'bed': round(num(p.get('bed_temper')), 1),
        'bed_target': round(num(p.get('bed_target_temper')), 1),
        'speed': SPEEDS[lang].get(int(num(p.get('spd_lvl'), 2)), ''),
        'wifi_dbm': int(num(str(p.get('wifi_signal', '0')).replace('dBm', ''), 0)),
        'nozzle_diameter': p.get('nozzle_diameter') or '',
        'nozzle_type': NOZZLES[lang].get(p.get('nozzle_type'), p.get('nozzle_type') or ''),
        'light': lights.get('chamber_light') == 'on',
        'print_error': int(num(p.get('print_error'), 0)),
        'hms_count': len(hms),
        'ams': units,
        'external': external,
        'camera_age': int(time.time() - CAMERA['at']) if CAMERA['jpeg'] else None,
    }


# ── HTTP ────────────────────────────────────────────────────────────────

class Handler(BaseHTTPRequestHandler):
    def do_GET(self):  # noqa: N802
        if self.client_address[0] not in ALLOWED:
            self.send_error(403)
            return
        path, _, query = self.path.partition('?')
        if path == '/bambu.json':
            lang = dict(kv.split('=', 1) for kv in query.split('&') if '=' in kv).get('lang', LANG)
            body = json.dumps(normalised(lang), ensure_ascii=False).encode()
            ctype = 'application/json; charset=utf-8'
        elif path == '/camera.jpg' and CAMERA['jpeg']:
            WANTED[0] = time.time()
            body, ctype = CAMERA['jpeg'], 'image/jpeg'
        else:
            self.send_error(404)
            return
        self.send_response(200)
        self.send_header('Content-Type', ctype)
        self.send_header('Content-Length', str(len(body)))
        self.send_header('Cache-Control', 'no-store')
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *_a):
        pass


def main():
    access_code()  # fail early with a clear message
    threading.Thread(target=mqtt_loop, daemon=True).start()
    threading.Thread(target=camera_loop, daemon=True).start()
    log(f'bambu-bridge for {PRINTER} on port {LISTEN[1]}; allowed: {", ".join(sorted(ALLOWED))}')
    ThreadingHTTPServer(LISTEN, Handler).serve_forever()


if __name__ == '__main__':
    main()
