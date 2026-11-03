"""Exercise the receiver with real TCP clients, without requiring Qt."""
import argparse
import ast
import datetime
import importlib.util
import marshal
from pathlib import Path
import socket
import tempfile
import threading
import time
import types
import unittest

RUN = None
PROCESS = None


class Signals:
    def __init__(self):
        self.messages = []
        self.changed = threading.Condition()

    def emit(self, full, raw):
        with self.changed:
            self.messages.append((full, raw))
            self.changed.notify_all()

    def contains(self, value, timeout=1.2):
        deadline = time.monotonic() + timeout
        with self.changed:
            while not any(raw == value for _, raw in self.messages):
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    return False
                self.changed.wait(remaining)
            return True


class Receiver:
    def __init__(self, directory):
        self.running = True
        self.server_socket = None
        self.recv_buffer = {}
        self.log_signal = Signals()
        self.log_capture_path = Path(directory) / 'test.log'
        with socket.socket() as probe:
            probe.bind(('127.0.0.1', 0))
            self.port = probe.getsockname()[1]

    def get_timestamp(self):
        return datetime.datetime.now().strftime('%H:%M:%S')

    def process_data(self, addr, data):
        return PROCESS(self, addr, data)


class ReceiverTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix='vitadeck-log-test-')
        self.receiver = Receiver(self.directory.name)
        self.clients = []
        self.thread = threading.Thread(target=RUN, args=(self.receiver,), daemon=True)
        self.thread.start()
        self.assertTrue(self.receiver.log_signal.contains('SERVER_START'))

    def connect(self):
        client = socket.create_connection(('127.0.0.1', self.receiver.port), timeout=1)
        self.clients.append(client)
        return client

    def tearDown(self):
        self.receiver.running = False
        for client in self.clients:
            client.close()
        self.thread.join(2)
        self.assertFalse(self.thread.is_alive(), 'receiver must stop with connected clients')
        self.directory.cleanup()

    def test_idle_kernel_connection_does_not_block_game_logs(self):
        self.connect()  # persistent CatLog/kernel stream, currently idle
        game = self.connect()
        line = '{"kind":"build_identity","build_tag":"poolfix-test"}'
        game.sendall((line + '\n').encode())
        self.assertTrue(self.receiver.log_signal.contains(line),
                        'a persistent idle client must not block the game stream')

    def test_same_ip_connections_keep_separate_partial_lines(self):
        first, second = self.connect(), self.connect()
        first.sendall(b'first:')
        second.sendall(b'second:complete\n')
        self.assertTrue(self.receiver.log_signal.contains('second:complete'))
        first.sendall(b'complete\n')
        self.assertTrue(self.receiver.log_signal.contains('first:complete'))
        raws = [raw for _, raw in self.receiver.log_signal.messages]
        self.assertNotIn('first:second:complete', raws)

    def test_split_utf8_and_final_line_survive_disconnect(self):
        client = self.connect()
        line = '版本：540p，日志恢复'
        encoded = line.encode()
        client.sendall(encoded[:1])
        time.sleep(.03)
        client.sendall(encoded[1:])
        client.shutdown(socket.SHUT_WR)
        self.assertTrue(self.receiver.log_signal.contains(line))

    def test_received_lines_are_saved_on_the_pc(self):
        client = self.connect()
        line = '{"kind":"frame_window","frame":600}'
        client.sendall((line + '\n').encode())
        self.assertTrue(self.receiver.log_signal.contains(line))
        self.assertIn(line, self.receiver.log_capture_path.read_text(encoding='utf-8'))


def main():
    global RUN, PROCESS
    parser = argparse.ArgumentParser()
    parser.add_argument('--baseline', type=Path)
    parser.add_argument('--baseline-marshal', type=Path,
                        help='extracted module code; run with its matching stable Python version')
    args, rest = parser.parse_known_args()
    if args.baseline_marshal:
        code = marshal.loads(args.baseline_marshal.read_bytes())
        cls = next(c for c in code.co_consts
                   if isinstance(c, types.CodeType) and c.co_name == 'LogServerThread')
        namespace = {'socket': socket}
        functions = {name: types.FunctionType(next(c for c in cls.co_consts
                     if isinstance(c, types.CodeType) and c.co_name == name), namespace)
                     for name in ('run', 'process_data')}
        RUN, PROCESS = functions['run'], functions['process_data']
    elif args.baseline:
        tree = ast.parse(args.baseline.read_text(encoding='utf-8-sig'))
        cls = next(n for n in tree.body if isinstance(n, ast.ClassDef) and n.name == 'LogServerThread')
        namespace = {'socket': socket}
        for name in ('run', 'process_data'):
            function = next(n for n in cls.body if isinstance(n, ast.FunctionDef) and n.name == name)
            exec(compile(ast.Module(body=[function], type_ignores=[]), str(args.baseline), 'exec'), namespace)
        RUN, PROCESS = namespace['run'], namespace['process_data']
    else:
        path = Path(__file__).with_name('logging_multiclient_fix.py')
        spec = importlib.util.spec_from_file_location('vitadeck_logging_fix_test', path)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        RUN = module.run_multiclient
    unittest.main(argv=[__file__, *rest])


if __name__ == '__main__':
    main()
