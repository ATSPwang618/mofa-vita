"""VitaDeck startup patch: receive concurrent CatLog streams without blocking.

Install this file in VitaDeck/components/custom/ and restart VitaDeck. The
documented custom-component discovery imports it before creating LoggingTab.
Only LogServerThread.run changes. Existing signals, controls, port settings,
and log formatting remain in the installed application.
"""
from __future__ import annotations

import codecs
from datetime import datetime
import os
from pathlib import Path
import selectors
import socket
import sys

PATCH_VERSION = 'vitadeck-multiclient-20261102'
COMPONENT_KEY = 'logging_multiclient_fix'
COMPONENT_LABEL = 'Logging receiver status'
MAX_CLIENTS = 64
MAX_PENDING_CHARS = 256 * 1024


class _Capture:
    """Small rolling UTF-8 writer; logging.handlers is absent from this EXE."""
    def __init__(self, path):
        self.path = path
        self.bytes = path.stat().st_size if path.exists() else 0
        self.stream = path.open('a', encoding='utf-8', newline='\n')

    def write(self, line):
        length = len((line + '\n').encode('utf-8'))
        if self.bytes + length > 8 * 1024 * 1024:
            self.stream.close()
            os.replace(self.path, self.path.with_name(self.path.name + '.1'))
            self.stream = self.path.open('w', encoding='utf-8', newline='\n')
            self.bytes = 0
        self.stream.write(line + '\n')
        self.stream.flush()
        self.bytes += length

    def close(self):
        self.stream.close()


def run_multiclient(self):
    selector = selectors.DefaultSelector()
    clients = {}
    capture = None
    try:
        path = getattr(self, 'log_capture_path', None)
        if path is None:
            path = Path(__file__).resolve().parents[2] / 'logs' / 'vita-console.log'
        path = Path(path)
        path.parent.mkdir(parents=True, exist_ok=True)
        capture = _Capture(path)
    except OSError as error:
        self.log_signal.emit(f'PC log capture unavailable: {error}', 'CAPTURE_ERROR')

    def emit(raw, status=None):
        nonlocal capture
        # Persist before delivering the asynchronous GUI signal, so a busy
        # logging widget cannot lose the only copy of a received window.
        if capture is not None:
            try:
                capture.write(raw)
            except OSError:
                capture.close()
                capture = None
        stamp = datetime.now().strftime('%H:%M:%S')
        self.log_signal.emit(f'{stamp} : {raw}', status if status is not None else raw)

    def consume(state, text):
        lines = (state['pending'] + text).split('\n')
        state['pending'] = lines.pop()
        for line in lines:
            emit(line.rstrip('\r'))
        if len(state['pending']) > MAX_PENDING_CHARS:
            emit(state['pending'][:MAX_PENDING_CHARS])
            emit('[LOG_LINE_TRUNCATED: client exceeded bounded line buffer]')
            state['pending'] = ''

    def close_client(client):
        state = clients.pop(client, None)
        if state is None:
            return
        try:
            selector.unregister(client)
        except (KeyError, ValueError, OSError):
            pass
        client.close()
        consume(state, state['decoder'].decode(b'', final=True))
        if state['pending']:
            emit(state['pending'].rstrip('\r'))
        address = state['address']
        emit(f'[{address[0]}:{address[1]}] Disconnected')

    try:
        server = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self.server_socket = server
        server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        server.bind(('0.0.0.0', self.port))
        server.listen(128)
        server.setblocking(False)
        selector.register(server, selectors.EVENT_READ)
        emit(f'--- Server Started on Port {self.port}; {PATCH_VERSION} ---', 'SERVER_START')
        while self.running:
            try:
                events = selector.select(.2)
            except (OSError, ValueError):
                break
            for key, _ in events:
                if key.fileobj is server:
                    try:
                        client, address = server.accept()
                    except (BlockingIOError, OSError):
                        continue
                    if len(clients) >= MAX_CLIENTS:
                        client.close()
                        emit('[Log client limit reached]', 'CLIENT_LIMIT')
                        continue
                    client.setblocking(False)
                    clients[client] = dict(address=address, pending='',
                        decoder=codecs.getincrementaldecoder('utf-8')(errors='replace'))
                    selector.register(client, selectors.EVENT_READ)
                    emit(f'[{address[0]}:{address[1]}] Connected')
                else:
                    client = key.fileobj
                    try:
                        data = client.recv(8192)
                    except BlockingIOError:
                        continue
                    except OSError:
                        data = b''
                    if data:
                        consume(clients[client], clients[client]['decoder'].decode(data))
                    else:
                        close_client(client)
    except OSError as error:
        emit(f'Bind/Listen Error: {error}', 'SERVER_ERROR')
    finally:
        for client in list(clients):
            close_client(client)
        selector.close()
        if self.server_socket is not None:
            self.server_socket.close()
        emit('--- Server Stopped ---', 'SERVER_STOP')
        if capture is not None:
            capture.close()


def install():
    # Avoid importing the bundled Qt application from command-line tests.
    module = sys.modules.get('components.logging')
    cls = getattr(module, 'LogServerThread', None)
    if cls is not None:
        cls.run = run_multiclient
        cls.multiclient_patch_version = PATCH_VERSION


def create_component(parent=None):
    # VitaDeck defaults custom status tabs to disabled. Startup import already
    # patches the built-in receiver, including when this optional tab is hidden.
    from PySide6.QtWidgets import QLabel
    label = QLabel(f'{PATCH_VERSION}\nConcurrent log reception is enabled.\n'
                   'PC capture: logs/vita-console.log (8 MiB + one backup)', parent)
    label.setWordWrap(True)
    return label


install()
