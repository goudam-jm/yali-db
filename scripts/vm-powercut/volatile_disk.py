"""Private oldstyle NBD export with a volatile write cache and explicit flush barriers.

Only newly created regular images are accepted. No host block devices or TCP listeners.
Single-connection, bounded, test-only subset: READ/WRITE/DISC/FLUSH and FUA.
"""
import errno
import os
from pathlib import Path
import socket
import stat
import struct
import threading

SIZE = 512 * 1024 * 1024
SECTOR = 512
MAX_REQUEST = 32 * 1024 * 1024
MAX_DIRTY = 192 * 1024 * 1024


def receive(connection, length):
    result = bytearray()
    while len(result) < length:
        data = connection.recv(length - len(result))
        if not data: raise EOFError('NBD client disconnected')
        result.extend(data)
    return bytes(result)


class Disk:
    def __init__(self, image, socket_path, trace, create=True):
        self.image = Path(image)
        flags = os.O_RDWR | os.O_NOFOLLOW | (os.O_CREAT | os.O_EXCL if create else 0)
        self.fd = os.open(image, flags, 0o600)
        info = os.fstat(self.fd)
        assert stat.S_ISREG(info.st_mode)
        if create: os.ftruncate(self.fd, SIZE)
        assert os.fstat(self.fd).st_size == SIZE
        self.socket_path = str(socket_path)
        self.trace = trace
        self.dirty = {}
        self.lock = threading.RLock()
        self.stopped = threading.Event()
        self.hit = threading.Event()
        self.release = threading.Event()
        self.armed = None
        self.ignore_flush = False
        self.counts = {'write': 0, 'flush': 0, 'fua': 0, 'ignored_flush': 0}
        self.flushed_bytes = 0
        self.error = None
        self.client = None
        self.listener = None
        self.thread = None

    def record(self, **row):
        import json
        if not self.trace.closed:
            self.trace.write(json.dumps(row) + '\n'); self.trace.flush()

    def read(self, offset, length):
        with self.lock:
            if self.stopped.is_set(): raise EOFError("virtual device power removed")
            data = bytearray(os.pread(self.fd, length, offset))
            for relative in range(0, length, SECTOR):
                block = self.dirty.get(offset + relative)
                if block is not None: data[relative:relative + SECTOR] = block
            return bytes(data)

    def write(self, offset, data):
        with self.lock:
            if self.stopped.is_set(): raise EOFError("virtual device power removed")
            for relative in range(0, len(data), SECTOR):
                self.dirty[offset + relative] = data[relative:relative + SECTOR]
            assert len(self.dirty) * SECTOR <= MAX_DIRTY, 'virtual disk cache budget exceeded'
            self.counts['write'] += 1

    def flush(self):
        with self.lock:
            if self.stopped.is_set(): raise EOFError("virtual device power removed")
            if self.ignore_flush:
                self.counts['ignored_flush'] += 1
                return
            # Merge adjacent sectors to avoid a syscall per sector during filesystem creation.
            start, chunk = None, bytearray()
            for offset, data in sorted(self.dirty.items()):
                if start is not None and offset != start + len(chunk):
                    self._persist(start, chunk); start, chunk = None, bytearray()
                if start is None: start = offset
                chunk.extend(data)
            if start is not None: self._persist(start, chunk)
            os.fsync(self.fd)
            self.flushed_bytes += len(self.dirty) * SECTOR
            self.dirty.clear()

    def _persist(self, offset, data):
        position = 0
        while position < len(data):
            count = os.pwrite(self.fd, data[position:], offset + position)
            if count <= 0: raise OSError('short backing image write')
            position += count

    def arm(self, event, occurrence=1):
        assert event in ('write', 'before-flush', 'after-flush')
        with self.lock:
            self.armed = [event, occurrence]
            self.hit.clear(); self.release.clear()

    def checkpoint(self, event):
        with self.lock:
            if self.armed is None or self.armed[0] != event: return
            self.armed[1] -= 1
            if self.armed[1] != 0: return
            self.armed = None
            self.record(event='cut-boundary', boundary=event, dirty_bytes=len(self.dirty) * SECTOR)
            self.hit.set()
        if not self.release.wait(120): raise TimeoutError('controller failed to cut at armed boundary')
        if self.stopped.is_set(): raise EOFError('virtual device power removed')

    def serve(self):
        self.listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.listener.bind(self.socket_path); self.listener.listen(1)
        self.listener.settimeout(0.25)
        self.thread = threading.Thread(target=self._serve, daemon=True); self.thread.start()

    def _serve(self):
        try:
            while not self.stopped.is_set():
                try:
                    self.client, _ = self.listener.accept(); break
                except socket.timeout: continue
            if self.client is None: return
            # Oldstyle negotiation: HAS_FLAGS | SEND_FLUSH | SEND_FUA. No trim/zero/cache claims.
            self.client.sendall(b'NBDMAGIC' + struct.pack('>QQI', 0x420281861253, SIZE, 13) + bytes(124))
            while not self.stopped.is_set():
                magic, flags, command, handle, offset, length = struct.unpack('>IHHQQI', receive(self.client, 28))
                assert magic == 0x25609513
                if command == 2: return
                assert length <= MAX_REQUEST
                payload = receive(self.client, length) if command == 1 else b''
                error, response = 0, b''
                if command not in (0, 1, 3) or flags & ~1 or (flags and command != 1):
                    error = errno.EINVAL
                elif command in (0, 1) and (offset % SECTOR or length % SECTOR or offset + length > SIZE):
                    error = errno.EINVAL
                else:
                    if command == 0:
                        with self.lock: response = self.read(offset, length)
                    elif command == 1:
                        with self.lock:
                            self.write(offset, payload)
                            self.record(event='write', offset=offset, length=length, fua=bool(flags & 1))
                        self.checkpoint('write')
                        if flags & 1:
                            self.checkpoint('before-flush')
                            with self.lock:
                                self.counts['fua'] += 1; self.flush()
                            self.checkpoint('after-flush')
                    else:
                        self.checkpoint('before-flush')
                        with self.lock:
                            self.counts['flush'] += 1; self.flush()
                            self.record(event='flush', ignored=self.ignore_flush)
                        self.checkpoint('after-flush')
                self.client.sendall(struct.pack('>IIQ', 0x67446698, error, handle) + response)
        except (EOFError, BrokenPipeError, ConnectionResetError):
            pass
        except BaseException as error:
            if not self.stopped.is_set():
                self.error = repr(error)
                self.record(event='backend-error', error=self.error)
        finally:
            if self.client: self.client.close()

    def power_cut(self):
        with self.lock:
            self.stopped.set()
            dropped = len(self.dirty) * SECTOR
            self.record(event='power-cut', discarded_bytes=dropped, counts=self.counts.copy())
            self.dirty.clear()
            self.release.set()
        if self.client:
            try: self.client.shutdown(socket.SHUT_RDWR)
            except OSError: pass
        if self.listener: self.listener.close()
        if self.thread:
            self.thread.join(timeout=5)
            assert not self.thread.is_alive(), 'NBD backend failed to stop'
        os.close(self.fd)
        assert self.error is None, self.error
        return {'discarded_bytes': dropped, 'counts': self.counts.copy(), 'flushed_bytes': self.flushed_bytes}
